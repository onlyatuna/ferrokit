// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::text::qcssparser::*;
use ferrokit_gui::tiny_skia::Color;

#[test]
fn test_qcss_parser_basic_selectors_and_declarations() {
    let qss = r#"
    /* Main window background */
    QWidget#CentralWidget {
        background-color: rgba(10, 20, 30, 0.5);
        border: 1px solid #445566;
        border-radius: 9px;
    }

    /* Labels */
    QLabel {
        color: #ddeeff;
        font-family: 'Segoe UI', sans-serif;
    }

    QLabel#TitleLabel {
        font-size: 11.5px;
        font-weight: 800;
        letter-spacing: 1.0px;
        color: #8899aa;
    }
    "#;

    let sheet = QCssStyleSheet::parse(qss);
    assert_eq!(sheet.rules.len(), 3);

    // Rule 1: QWidget#CentralWidget
    let r1 = &sheet.rules[0];
    assert_eq!(r1.selectors.len(), 1);
    assert_eq!(r1.selectors[0].element_name, Some("QWidget".to_string()));
    assert_eq!(r1.selectors[0].id, Some("CentralWidget".to_string()));
    assert_eq!(r1.selectors[0].specificity(), 101); // 100 for ID + 1 for type
    assert_eq!(r1.declarations.len(), 3);
    assert_eq!(r1.declarations[0].property, QCssProperty::BackgroundColor);
    assert_eq!(
        r1.declarations[0].value,
        QCssValue::Color(Color::from_rgba8(10, 20, 30, 127))
    );
    assert_eq!(r1.declarations[2].property, QCssProperty::BorderRadius);
    assert_eq!(r1.declarations[2].value, QCssValue::Length(9.0));

    // Rule 2: QLabel
    let r2 = &sheet.rules[1];
    assert_eq!(r2.selectors[0].element_name, Some("QLabel".to_string()));
    assert_eq!(r2.selectors[0].id, None);
    assert_eq!(r2.selectors[0].specificity(), 1);

    // Rule 3: QLabel#TitleLabel
    let r3 = &sheet.rules[2];
    assert_eq!(r3.selectors[0].element_name, Some("QLabel".to_string()));
    assert_eq!(r3.selectors[0].id, Some("TitleLabel".to_string()));
    assert_eq!(r3.declarations[0].property, QCssProperty::FontSize);
    assert_eq!(r3.declarations[0].value, QCssValue::Length(11.5));
}

#[test]
fn test_qcss_parser_progress_bar_and_sub_control() {
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

    let sheet = QCssStyleSheet::parse(qss);
    assert_eq!(sheet.rules.len(), 2);

    let pb_rule = &sheet.rules[0];
    assert_eq!(pb_rule.selectors[0].element_name, Some("QProgressBar".to_string()));
    assert_eq!(pb_rule.selectors[0].sub_control, None);

    let mut min_h = None;
    let mut max_h = None;
    for decl in &pb_rule.declarations {
        if decl.property == QCssProperty::MinHeight {
            min_h = Some(&decl.value);
        } else if decl.property == QCssProperty::MaxHeight {
            max_h = Some(&decl.value);
        }
    }
    assert_eq!(min_h, Some(&QCssValue::Length(6.0)));
    assert_eq!(max_h, Some(&QCssValue::Length(6.0)));

    let chunk_rule = &sheet.rules[1];
    assert_eq!(chunk_rule.selectors[0].element_name, Some("QProgressBar".to_string()));
    assert_eq!(chunk_rule.selectors[0].sub_control, Some("chunk".to_string()));
    assert_eq!(chunk_rule.declarations.len(), 2);
    assert_eq!(chunk_rule.declarations[1].property, QCssProperty::BackgroundColor);
    assert_eq!(
        chunk_rule.declarations[1].value,
        QCssValue::Color(Color::from_rgba8(34, 187, 119, 255))
    );
}

#[test]
fn test_qcss_parser_pseudo_states_and_attributes() {
    let qss = r#"
    QPushButton#ToggleBtn:hover {
        background-color: rgba(255, 255, 255, 0.5);
        color: #3399ff;
    }

    QLabel[state="muted"] {
        color: rgba(235, 235, 245, 0.32);
    }
    "#;

    let sheet = QCssStyleSheet::parse(qss);
    assert_eq!(sheet.rules.len(), 2);

    let btn_hover = &sheet.rules[0];
    assert_eq!(btn_hover.selectors[0].element_name, Some("QPushButton".to_string()));
    assert_eq!(btn_hover.selectors[0].id, Some("ToggleBtn".to_string()));
    assert_eq!(btn_hover.selectors[0].pseudo_states, vec!["hover".to_string()]);
    assert_eq!(btn_hover.selectors[0].specificity(), 111); // 100(id) + 10(pseudo) + 1(type)

    let attr_rule = &sheet.rules[1];
    assert_eq!(attr_rule.selectors[0].element_name, Some("QLabel".to_string()));
    assert_eq!(
        attr_rule.selectors[0].attributes,
        vec![("state".to_string(), "muted".to_string())]
    );
    assert_eq!(attr_rule.selectors[0].specificity(), 11); // 10(attr) + 1(type)
}

#[test]
fn test_qcss_parser_multi_selectors_and_edges() {
    let qss = r#"
    QLabel#SectionTitle, QLabel#RowLabel {
        padding: 1px 4px;
        margin: 5px 10px;
    }
    "#;

    let sheet = QCssStyleSheet::parse(qss);
    assert_eq!(sheet.rules.len(), 1);
    let rule = &sheet.rules[0];
    assert_eq!(rule.selectors.len(), 2);
    assert_eq!(rule.selectors[0].id, Some("SectionTitle".to_string()));
    assert_eq!(rule.selectors[1].id, Some("RowLabel".to_string()));

    assert_eq!(rule.declarations[0].property, QCssProperty::Padding);
    assert_eq!(rule.declarations[0].value, QCssValue::Edges([1.0, 4.0, 1.0, 4.0]));

    assert_eq!(rule.declarations[1].property, QCssProperty::Margin);
    assert_eq!(rule.declarations[1].value, QCssValue::Edges([5.0, 10.0, 5.0, 10.0]));
}
#[test]
fn test_qcss_8_digit_hex_color_is_argb() {
    // Qt QColor::fromString("#11223344") interprets 8-digit hex as #AARRGGBB:
    // alpha=0x11 (17), red=0x22 (34), green=0x33 (51), blue=0x44 (68).
    use ferrokit_gui::text::qcssparser::parse_color;
    let color = parse_color("#11223344").expect("valid 8-digit hex color");
    let u = color.to_color_u8();
    assert_eq!((u.alpha(), u.red(), u.green(), u.blue()), (17, 34, 51, 68));
}

/// `rgba(r, g, b, <0..1>)` truncates the alpha: it is multiplied by 255 and cut to an int
/// (qcssparser.cpp:826-828). PySide6 6.11.2 painted these over a transparent pixmap:
/// 0.12 -> 0x1e, 0.5 -> 0x7f, 0.97 -> 0xf7, 0.07 -> 0x11, 0.1 -> 0x19, 0.35 -> 0x59.
#[test]
fn test_qcss_rgba_fractional_alpha_truncates_like_qt() {
    use ferrokit_gui::text::qcssparser::parse_color;
    for (text, alpha) in [
        ("0.12", 30),
        ("0.5", 127),
        ("0.97", 247),
        ("0.07", 17),
        ("0.1", 25),
        ("0.35", 89),
        ("1", 255),
        ("0", 0),
    ] {
        let color = parse_color(&format!("rgba(255, 255, 255, {text})")).expect("valid rgba color");
        assert_eq!(color.to_color_u8().alpha(), alpha, "rgba alpha {text}");
    }
}
