// SPDX-License-Identifier: LGPL-3.0-only
//! Longhand margin properties (margin-top, margin-right, margin-bottom, margin-left).
//!
//! Qt reference: `qcssparser.cpp:138-141` parses `margin-top`, `margin-right`, `margin-bottom`, `margin-left`.
//! `qstylesheetstyle.cpp` applies individual margin sides to the box model.

use ferrokit_widgets::style::stylesheet::{QStyleSheetStyle, WidgetStyleContext};

#[test]
fn margin_longhand_properties_are_applied() {
    let qss = r#"
    QMenu {
        margin-top: 4px;
        margin-right: 8px;
        margin-bottom: 12px;
        margin-left: 16px;
    }
    "#;
    let style = QStyleSheetStyle::parse(qss);
    let ctx = WidgetStyleContext {
        type_name: "QMenu",
        object_name: "",
        pseudo_states: &[],
        sub_control: None,
        attributes: &[],
    };
    let res = style.resolve(&ctx);
    assert_eq!(
        res.margin,
        Some([4.0, 8.0, 12.0, 16.0]),
        "margin longhands must be applied to resolved style"
    );
}

#[test]
fn margin_shorthand_and_longhand_override() {
    let qss = r#"
    QMenu {
        margin: 5px;
        margin-top: 20px;
    }
    "#;
    let style = QStyleSheetStyle::parse(qss);
    let ctx = WidgetStyleContext {
        type_name: "QMenu",
        object_name: "",
        pseudo_states: &[],
        sub_control: None,
        attributes: &[],
    };
    let res = style.resolve(&ctx);
    assert_eq!(
        res.margin,
        Some([20.0, 5.0, 5.0, 5.0]),
        "longhand margin-top should override shorthand top component"
    );
}
