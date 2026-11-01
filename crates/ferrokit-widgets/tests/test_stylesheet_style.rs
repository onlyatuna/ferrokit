// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::object::QObject;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_widgets::Widget;
use ferrokit_widgets::label::Label;
use ferrokit_widgets::style::stylesheet::{QStyleSheetStyle, WidgetStyleContext};

#[test]
fn test_stylesheet_style_resolution_and_specificity() {
    let qss = r#"
    QLabel {
        color: #ddeeff;
        font-size: 12px;
    }

    QLabel#TitleLabel {
        color: #8899aa;
        font-size: 11.5px;
        font-weight: 800;
    }
    "#;

    let style = QStyleSheetStyle::parse(qss);

    // Context 1: Generic QLabel
    let ctx_generic = WidgetStyleContext {
        type_name: "QLabel",
        object_name: "",
        pseudo_states: &[],
        sub_control: None,
        attributes: &[],
    };
    let res_generic = style.resolve(&ctx_generic);
    assert_eq!(
        res_generic.color,
        Some(Color::from_rgba8(221, 238, 255, 255))
    );
    assert_eq!(res_generic.font_size, Some(12.0));
    assert_eq!(res_generic.font_weight, None);

    // Context 2: QLabel#TitleLabel (ID specificity should override generic)
    let ctx_id = WidgetStyleContext {
        type_name: "QLabel",
        object_name: "TitleLabel",
        pseudo_states: &[],
        sub_control: None,
        attributes: &[],
    };
    let res_id = style.resolve(&ctx_id);
    assert_eq!(res_id.color, Some(Color::from_rgba8(136, 153, 170, 255)));
    assert_eq!(res_id.font_size, Some(11.5));
    assert_eq!(res_id.font_weight, Some(800));

    // Verify Label styled_font maps 800 to FontWeight::ExtraBold (QFont::ExtraBold = 800)
    let mut lbl = Label::new("Title");
    lbl.set_object_name("TitleLabel");
    lbl.set_style_sheet("QLabel#TitleLabel { font-weight: 800; }");
    assert_eq!(
        lbl.effective_font().weight,
        ferrokit_gui::text::FontWeight::ExtraBold,
        "QSS font-weight 800 must map to FontWeight::ExtraBold (qfont.h:72)"
    );
}

#[test]
fn test_stylesheet_style_progress_bar_and_chunk() {
    let qss = r#"
    QProgressBar {
        background-color: rgba(255, 255, 255, 0.1);
        border: none;
        border-radius: 2px;
        min-height: 6px;
        max-height: 6px;
    }

    QProgressBar::chunk {
        border-radius: 2px;
        background-color: #22bb77;
    }
    "#;

    let style = QStyleSheetStyle::parse(qss);

    // 1. Groove (no sub-control)
    let ctx_groove = WidgetStyleContext {
        type_name: "QProgressBar",
        object_name: "",
        pseudo_states: &[],
        sub_control: None,
        attributes: &[],
    };
    let res_groove = style.resolve(&ctx_groove);
    assert_eq!(res_groove.min_height, Some(6));
    assert_eq!(res_groove.max_height, Some(6));
    assert_eq!(res_groove.border_radius, Some(2.0));
    assert_eq!(
        res_groove.background_color,
        Some(Color::from_rgba8(255, 255, 255, 25))
    );

    // 2. Chunk (sub-control = "chunk")
    let ctx_chunk = WidgetStyleContext {
        type_name: "QProgressBar",
        object_name: "",
        pseudo_states: &[],
        sub_control: Some("chunk"),
        attributes: &[],
    };
    let res_chunk = style.resolve(&ctx_chunk);
    assert_eq!(res_chunk.border_radius, Some(2.0));
    assert_eq!(
        res_chunk.background_color,
        Some(Color::from_rgba8(34, 187, 119, 255))
    );
}

#[test]
fn test_stylesheet_style_cascaded_resolution() {
    // Global window stylesheet has the 6px height and groove background
    let global_qss = r#"
    QProgressBar {
        background-color: rgba(255, 255, 255, 0.1);
        min-height: 6px;
        max-height: 6px;
    }
    "#;
    let global_style = QStyleSheetStyle::parse(global_qss);

    // Local widget stylesheet ONLY changes chunk color dynamically
    let local_qss = r#"
    QProgressBar::chunk {
        background-color: #ef4444;
    }
    "#;
    let local_style = QStyleSheetStyle::parse(local_qss);

    // Context for chunk: local chunk color overrides, but global 6px stays intact!
    let ctx_groove = WidgetStyleContext {
        type_name: "QProgressBar",
        object_name: "",
        pseudo_states: &[],
        sub_control: None,
        attributes: &[],
    };
    let res =
        QStyleSheetStyle::resolve_cascaded(Some(&local_style), Some(&global_style), &ctx_groove);
    assert_eq!(res.min_height, Some(6));
    assert_eq!(res.max_height, Some(6));
    assert_eq!(
        res.background_color,
        Some(Color::from_rgba8(255, 255, 255, 25))
    );

    let ctx_chunk = WidgetStyleContext {
        type_name: "QProgressBar",
        object_name: "",
        pseudo_states: &[],
        sub_control: Some("chunk"),
        attributes: &[],
    };
    let res_chunk =
        QStyleSheetStyle::resolve_cascaded(Some(&local_style), Some(&global_style), &ctx_chunk);
    assert_eq!(
        res_chunk.background_color,
        Some(Color::from_rgba8(239, 68, 68, 255))
    );
}

#[test]
fn test_stylesheet_style_pseudo_states_hover() {
    let qss = r#"
    QPushButton#ToggleBtn {
        background-color: transparent;
        color: #8899aa;
        min-width: 18px;
        max-height: 18px;
    }

    QPushButton#ToggleBtn:hover {
        background-color: rgba(255, 255, 255, 0.5);
        color: #3399ff;
    }
    "#;

    let style = QStyleSheetStyle::parse(qss);

    // Normal state
    let ctx_normal = WidgetStyleContext {
        type_name: "QPushButton",
        object_name: "ToggleBtn",
        pseudo_states: &[],
        sub_control: None,
        attributes: &[],
    };
    let res_normal = style.resolve(&ctx_normal);
    assert_eq!(res_normal.min_width, Some(18));
    assert_eq!(res_normal.max_height, Some(18));
    assert_eq!(
        res_normal.color,
        Some(Color::from_rgba8(136, 153, 170, 255))
    );

    // Hover state
    let ctx_hover = WidgetStyleContext {
        type_name: "QPushButton",
        object_name: "ToggleBtn",
        pseudo_states: &["hover"],
        sub_control: None,
        attributes: &[],
    };
    let res_hover = style.resolve(&ctx_hover);
    assert_eq!(res_hover.color, Some(Color::from_rgba8(51, 153, 255, 255)));
    assert_eq!(
        res_hover.background_color,
        Some(Color::from_rgba8(255, 255, 255, 127))
    );
}
