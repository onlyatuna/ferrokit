// SPDX-License-Identifier: LGPL-3.0-only
//! Stylesheet cascade and box-model resolution engine.

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
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub font_size: Option<f32>,
    pub font_weight: Option<u16>,
    pub font_families: Option<Vec<String>>,
    pub letter_spacing: Option<f32>,
    pub padding: Option<[f32; 4]>,
    pub margin: Option<[f32; 4]>,
    pub text_align: Option<String>,
}

impl ResolvedStyle {
    /// Returns the first font family in `font_families` registered in the global font database,
    /// falling back to the first listed family if none are found.
    pub fn font_family(&self) -> Option<String> {
        self.font_families.as_ref().and_then(|list| {
            list.iter()
                .find(|f| {
                    ferrokit_gui::text::font_database::with_global_font_database(|db| {
                        db.has_family(f)
                    })
                })
                .cloned()
                .or_else(|| list.first().cloned())
        })
    }

    /// Computes total horizontal and vertical extra box size from border width and padding.
    pub fn box_extra(&self) -> (i32, i32) {
        let border = self.border_width.unwrap_or(0.0).max(0.0);
        let [pt, pr, pb, pl] = self.padding.unwrap_or([0.0; 4]);
        (
            ((2.0 * border) + pl + pr).round() as i32,
            ((2.0 * border) + pt + pb).round() as i32,
        )
    }

    /// Computes minimum outer box dimensions including border and padding.
    pub fn min_box_size(&self) -> (i32, i32) {
        let (ex, ey) = self.box_extra();
        (
            self.min_width
                .map_or(0, |m| m.max(self.width.unwrap_or(-1)) + ex),
            self.min_height
                .map_or(0, |m| m.max(self.height.unwrap_or(-1)) + ey),
        )
    }

    /// Computes maximum outer box dimensions including border and padding.
    pub fn max_box_size(&self) -> (i32, i32) {
        let (ex, ey) = self.box_extra();
        (
            self.max_width
                .map_or(16_777_215, |m| m.min(self.width.unwrap_or(16_777_215)) + ex),
            self.max_height.map_or(16_777_215, |m| {
                m.min(self.height.unwrap_or(16_777_215)) + ey
            }),
        )
    }

    /// Adjusts a base `(width, height)` pair against resolved `width`, `height`, `max_*`, and `min_*` constraints.
    pub fn adjust_size(&self, (w, h): (i32, i32)) -> (i32, i32) {
        let mut rw = self.width.unwrap_or(w);
        let mut rh = self.height.unwrap_or(h);
        if let Some(max_w) = self.max_width {
            rw = rw.min(max_w);
        }
        if let Some(max_h) = self.max_height {
            rh = rh.min(max_h);
        }
        rw = rw.max(self.min_width.unwrap_or(-1));
        rh = rh.max(self.min_height.unwrap_or(-1));
        (rw, rh)
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

/// Style sheet engine instance.
#[derive(Debug, Clone, Default)]
pub struct QStyleSheetStyle {
    sheet: Arc<QCssStyleSheet>,
}

impl QStyleSheetStyle {
    pub fn new(sheet: Arc<QCssStyleSheet>) -> Self {
        Self { sheet }
    }

    pub fn parse(qss: &str) -> Self {
        Self {
            sheet: Arc::new(QCssStyleSheet::parse(qss)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sheet.is_empty()
    }

    pub fn resolve(&self, ctx: &WidgetStyleContext) -> ResolvedStyle {
        Self::resolve_chain(&[self], ctx)
    }

    /// Resolves a slice of stylesheets in cascade order (later sheets have higher `depth`).
    pub fn resolve_chain(sheets: &[&QStyleSheetStyle], ctx: &WidgetStyleContext) -> ResolvedStyle {
        let mut matched: Vec<(usize, u32, &QCssDeclaration)> = Vec::new();

        for (depth, sheet) in sheets.iter().enumerate() {
            for rule in &sheet.sheet.rules {
                let max_spec = rule
                    .selectors
                    .iter()
                    .filter(|sel| matches_selector(sel, ctx))
                    .map(QCssBasicSelector::specificity)
                    .max();
                if let Some(score) = max_spec {
                    for decl in &rule.declarations {
                        matched.push((depth, score, decl));
                    }
                }
            }
        }

        matched.sort_by_key(|(depth, score, _)| (*depth, *score));

        let mut style = ResolvedStyle::default();
        for (_, _, decl) in matched {
            apply_declaration(&mut style, decl);
        }
        style
    }

    /// Resolves an optional `local` stylesheet over an optional `fallback` stylesheet.
    pub fn resolve_cascaded(
        local: Option<&QStyleSheetStyle>,
        fallback: Option<&QStyleSheetStyle>,
        ctx: &WidgetStyleContext,
    ) -> ResolvedStyle {
        let mut chain: Vec<&QStyleSheetStyle> = Vec::with_capacity(2);
        if let Some(fb) = fallback {
            chain.push(fb);
        }
        if let Some(loc) = local {
            chain.push(loc);
        }
        Self::resolve_chain(&chain, ctx)
    }
}

fn matches_selector(selector: &QCssBasicSelector, ctx: &WidgetStyleContext) -> bool {
    if selector.sub_control.as_deref() != ctx.sub_control {
        return false;
    }

    if let Some(ref id) = selector.id
        && id != ctx.object_name
    {
        return false;
    }

    if let Some(ref elem) = selector.element_name
        && elem != "*"
    {
        let elem_matches = if ctx.type_name == "QToolTip" {
            elem == "QToolTip"
        } else {
            elem == "QObject"
                || elem == "QWidget"
                || ctx.type_name == elem
                || (ctx.type_name == "QLabel" && elem == "QFrame")
                || (ctx.type_name == "QPushButton" && elem == "QAbstractButton")
        };
        if !elem_matches {
            return false;
        }
    }

    for state in &selector.pseudo_states {
        if !ctx.pseudo_states.iter().any(|s| s == state) {
            return false;
        }
    }

    for (k, v) in &selector.attributes {
        if !ctx.attributes.iter().any(|(ak, av)| ak == k && av == v) {
            return false;
        }
    }

    true
}

fn apply_declaration(style: &mut ResolvedStyle, decl: &QCssDeclaration) {
    match &decl.property {
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
        QCssProperty::BorderColor => {
            if let QCssValue::Color(c) = decl.value {
                style.border_color = Some(c);
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
        QCssProperty::BorderWidth => {
            if let QCssValue::Length(v) = decl.value {
                style.border_width = Some(v);
            }
        }
        QCssProperty::BorderRadius => {
            if let QCssValue::Length(v) = decl.value {
                style.border_radius = Some(v);
            }
        }
        QCssProperty::FontSize => {
            if let QCssValue::Length(v) = decl.value {
                style.font_size = Some(v);
            }
        }
        QCssProperty::LetterSpacing => {
            if let QCssValue::Length(v) = decl.value {
                style.letter_spacing = Some(v);
            }
        }
        QCssProperty::BorderStyle => {
            if let QCssValue::Identifier(ref s) = decl.value {
                style.border_style = Some(s.clone());
            }
        }
        QCssProperty::TextAlign => {
            if let QCssValue::Identifier(ref s) = decl.value {
                style.text_align = Some(s.clone());
            }
        }
        QCssProperty::MinHeight => {
            if let QCssValue::Length(v) = decl.value {
                style.min_height = Some(v.round() as i32);
            }
        }
        QCssProperty::MaxHeight => {
            if let QCssValue::Length(v) = decl.value {
                style.max_height = Some(v.round() as i32);
            }
        }
        QCssProperty::Height => {
            if let QCssValue::Length(v) = decl.value {
                style.height = Some(v.round() as i32);
            }
        }
        QCssProperty::MinWidth => {
            if let QCssValue::Length(v) = decl.value {
                style.min_width = Some(v.round() as i32);
            }
        }
        QCssProperty::MaxWidth => {
            if let QCssValue::Length(v) = decl.value {
                style.max_width = Some(v.round() as i32);
            }
        }
        QCssProperty::Width => {
            if let QCssValue::Length(v) = decl.value {
                style.width = Some(v.round() as i32);
            }
        }
        QCssProperty::FontWeight => match &decl.value {
            QCssValue::Number(n) => {
                style.font_weight = Some(n.round() as u16);
            }
            QCssValue::Identifier(s) if s.eq_ignore_ascii_case("bold") => {
                style.font_weight = Some(700);
            }
            QCssValue::Identifier(s) if s.eq_ignore_ascii_case("normal") => {
                style.font_weight = Some(400);
            }
            _ => {}
        },
        QCssProperty::FontFamily => {
            if let QCssValue::FontFamilies(ref list) = decl.value {
                style.font_families = Some(list.clone());
            }
        }
        QCssProperty::Padding => {
            if let QCssValue::Edges(e) = decl.value {
                style.padding = Some(e);
            }
        }
        QCssProperty::Margin => {
            if let QCssValue::Edges(e) = decl.value {
                style.margin = Some(e);
            }
        }
        QCssProperty::PaddingTop => update_edge(&mut style.padding, 0, &decl.value),
        QCssProperty::PaddingRight => update_edge(&mut style.padding, 1, &decl.value),
        QCssProperty::PaddingBottom => update_edge(&mut style.padding, 2, &decl.value),
        QCssProperty::PaddingLeft => update_edge(&mut style.padding, 3, &decl.value),
        QCssProperty::MarginTop => update_edge(&mut style.margin, 0, &decl.value),
        QCssProperty::MarginRight => update_edge(&mut style.margin, 1, &decl.value),
        QCssProperty::MarginBottom => update_edge(&mut style.margin, 2, &decl.value),
        QCssProperty::MarginLeft => update_edge(&mut style.margin, 3, &decl.value),
        QCssProperty::Custom(_) => {}
    }
}

fn update_edge(target: &mut Option<[f32; 4]>, index: usize, value: &QCssValue) {
    if let QCssValue::Length(v) = value {
        let edges = target.get_or_insert([0.0; 4]);
        edges[index] = *v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascade_specificity_and_depth_order() {
        let fallback = QStyleSheetStyle::parse(
            "QWidget { color: black; background-color: white; } #btn { color: red; }",
        );
        let local = QStyleSheetStyle::parse(
            "QPushButton { color: blue; padding: 4px 8px; border: 2px solid green; }",
        );
        let ctx = WidgetStyleContext {
            type_name: "QPushButton",
            object_name: "btn",
            pseudo_states: &[],
            sub_control: None,
            attributes: &[],
        };
        let resolved = QStyleSheetStyle::resolve_cascaded(Some(&local), Some(&fallback), &ctx);
        assert_eq!(resolved.color, Some(Color::from_rgba8(0, 0, 255, 255)));
        assert_eq!(
            resolved.background_color,
            Some(Color::from_rgba8(255, 255, 255, 255))
        );
        assert_eq!(resolved.box_extra(), (20, 12));
    }

    #[test]
    fn tooltip_and_inherited_element_matching() {
        let sheet = QStyleSheetStyle::parse(
            "QWidget { color: red; } QFrame { background-color: blue; } QToolTip { color: white; }",
        );
        let label_ctx = WidgetStyleContext {
            type_name: "QLabel",
            ..Default::default()
        };
        let label_style = sheet.resolve(&label_ctx);
        assert_eq!(label_style.color, Some(Color::from_rgba8(255, 0, 0, 255)));
        assert_eq!(
            label_style.background_color,
            Some(Color::from_rgba8(0, 0, 255, 255))
        );

        let tip_ctx = WidgetStyleContext {
            type_name: "QToolTip",
            ..Default::default()
        };
        let tip_style = sheet.resolve(&tip_ctx);
        assert_eq!(tip_style.color, Some(Color::from_rgba8(255, 255, 255, 255)));
    }
}
