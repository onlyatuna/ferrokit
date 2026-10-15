// SPDX-License-Identifier: LGPL-3.0-only
//! Parity tests for `setAttribute` / `testAttribute`.

use ferrokit_widgets::widget::{EmptyWidget, Widget, WidgetAttribute};

#[test]
fn widget_attribute_get_and_set() {
    let w = EmptyWidget::new();

    // MouseTracking
    assert!(!w.test_attribute(WidgetAttribute::MouseTracking));
    w.set_attribute(WidgetAttribute::MouseTracking, true);
    assert!(w.test_attribute(WidgetAttribute::MouseTracking));
    assert!(w.widget_base().has_mouse_tracking());

    // TransparentForMouseEvents
    assert!(!w.test_attribute(WidgetAttribute::TransparentForMouseEvents));
    w.set_attribute(WidgetAttribute::TransparentForMouseEvents, true);
    assert!(w.test_attribute(WidgetAttribute::TransparentForMouseEvents));
    assert!(w.widget_base().is_transparent_for_mouse_events());

    // NoMousePropagation
    assert!(!w.test_attribute(WidgetAttribute::NoMousePropagation));
    w.set_attribute(WidgetAttribute::NoMousePropagation, true);
    assert!(w.test_attribute(WidgetAttribute::NoMousePropagation));
    assert!(w.widget_base().has_no_mouse_propagation());

    // AlwaysShowToolTips
    assert!(!w.test_attribute(WidgetAttribute::AlwaysShowToolTips));
    w.set_attribute(WidgetAttribute::AlwaysShowToolTips, true);
    assert!(w.test_attribute(WidgetAttribute::AlwaysShowToolTips));
    assert!(w.always_show_tool_tips());

    // Disabled
    assert!(!w.test_attribute(WidgetAttribute::Disabled));
    assert!(w.is_enabled());
    w.set_attribute(WidgetAttribute::Disabled, true);
    assert!(w.test_attribute(WidgetAttribute::Disabled));
    assert!(!w.is_enabled());
}
