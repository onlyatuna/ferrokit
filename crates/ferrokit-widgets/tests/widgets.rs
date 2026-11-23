// SPDX-License-Identifier: LGPL-3.0-only
//! Single integration-test binary (one link step instead of one per file).

// Several test files each include tests/support/probe_widget.rs on purpose, so every module keeps
// its own copy of the probe types (and its own statics).
#![allow(clippy::duplicate_mod)]

#[path = "layout_qt_oracle.rs"]
mod layout_qt_oracle;
#[path = "test_action_reentrancy.rs"]
mod test_action_reentrancy;
#[path = "test_arc_progress_and_custom_paint.rs"]
mod test_arc_progress_and_custom_paint;
#[path = "test_attribute_selector.rs"]
mod test_attribute_selector;
#[path = "test_box_layout_add_stretch_zero.rs"]
mod test_box_layout_add_stretch_zero;
#[path = "test_box_layout_spacing.rs"]
mod test_box_layout_spacing;
#[path = "test_button_block.rs"]
mod test_button_block;
#[path = "test_button_minimum_size_hint.rs"]
mod test_button_minimum_size_hint;
#[path = "test_command_flush_borrowed.rs"]
mod test_command_flush_borrowed;
#[path = "test_enabled_change.rs"]
mod test_enabled_change;
#[path = "test_explicit_size_and_hfw.rs"]
mod test_explicit_size_and_hfw;
#[path = "test_focus_disable.rs"]
mod test_focus_disable;
#[path = "test_focus_dispatch.rs"]
mod test_focus_dispatch;
#[path = "test_frame_extreme_line_width.rs"]
mod test_frame_extreme_line_width;
#[path = "test_frame_size_hint.rs"]
mod test_frame_size_hint;
#[path = "test_grid_layout_accessors.rs"]
mod test_grid_layout_accessors;
#[path = "test_grid_layout_python_semantics.rs"]
mod test_grid_layout_python_semantics;
#[path = "test_hit_test_dpi_old_dpr.rs"]
mod test_hit_test_dpi_old_dpr;
#[path = "test_label_box_model.rs"]
mod test_label_box_model;
#[path = "test_label_carriage_return.rs"]
mod test_label_carriage_return;
#[path = "test_label_focus_policy.rs"]
mod test_label_focus_policy;
#[path = "test_label_letter_spacing_hint.rs"]
mod test_label_letter_spacing_hint;
#[path = "test_label_natural_width_hint.rs"]
mod test_label_natural_width_hint;
#[path = "test_label_overflow_alignment.rs"]
mod test_label_overflow_alignment;
#[path = "test_label_separators.rs"]
mod test_label_separators;
#[path = "test_label_tabs.rs"]
mod test_label_tabs;
#[path = "test_layout_alignment.rs"]
mod test_layout_alignment;
#[path = "test_layout_default_metrics.rs"]
mod test_layout_default_metrics;
#[path = "test_layout_grid_alignment.rs"]
mod test_layout_grid_alignment;
#[path = "test_layout_reentrancy.rs"]
mod test_layout_reentrancy;
#[path = "test_layout_size_constraint.rs"]
mod test_layout_size_constraint;
#[path = "test_layout_stretch_minimum.rs"]
mod test_layout_stretch_minimum;
#[path = "test_margin_longhand.rs"]
mod test_margin_longhand;
#[path = "test_menu_key_normalization.rs"]
mod test_menu_key_normalization;
#[path = "test_mouse_attributes_parity.rs"]
mod test_mouse_attributes_parity;
#[path = "test_nested_layout_maximum.rs"]
mod test_nested_layout_maximum;
#[path = "test_progress_bar_drop_in_callback.rs"]
mod test_progress_bar_drop_in_callback;
#[path = "test_progress_bar_size_hint.rs"]
mod test_progress_bar_size_hint;
#[path = "test_qss_min_max_box.rs"]
mod test_qss_min_max_box;
#[path = "test_r3_visibility_focus.rs"]
mod test_r3_visibility_focus;
#[path = "test_scroll_area_content_resize.rs"]
mod test_scroll_area_content_resize;
#[path = "test_scroll_area_input.rs"]
mod test_scroll_area_input;
#[path = "test_scrollbar_extreme_range.rs"]
mod test_scrollbar_extreme_range;
#[path = "test_show_hide_borrowed_child.rs"]
mod test_show_hide_borrowed_child;
#[path = "test_show_hide_hooks.rs"]
mod test_show_hide_hooks;
#[path = "test_stacked_layout_geometry.rs"]
mod test_stacked_layout_geometry;
#[path = "test_stacked_layout_hint.rs"]
mod test_stacked_layout_hint;
#[path = "test_stacked_layout_spacing.rs"]
mod test_stacked_layout_spacing;
#[path = "test_style_and_commands.rs"]
mod test_style_and_commands;
#[path = "test_stylesheet_disabled.rs"]
mod test_stylesheet_disabled;
#[path = "test_stylesheet_enabled.rs"]
mod test_stylesheet_enabled;
#[path = "test_stylesheet_focus.rs"]
mod test_stylesheet_focus;
#[path = "test_stylesheet_focus_label_bar.rs"]
mod test_stylesheet_focus_label_bar;
#[path = "test_stylesheet_style.rs"]
mod test_stylesheet_style;
#[path = "test_stylesheet_type_inheritance.rs"]
mod test_stylesheet_type_inheritance;
#[path = "test_tab_key_hook.rs"]
mod test_tab_key_hook;
#[path = "test_tool_tip_change_event.rs"]
mod test_tool_tip_change_event;
#[path = "test_widget_attributes_api.rs"]
mod test_widget_attributes_api;
#[path = "test_widget_default_size_hint.rs"]
mod test_widget_default_size_hint;
#[path = "test_widget_enabled.rs"]
mod test_widget_enabled;
#[path = "test_widget_partial_update.rs"]
mod test_widget_partial_update;
#[path = "test_widget_property_dirty.rs"]
mod test_widget_property_dirty;
