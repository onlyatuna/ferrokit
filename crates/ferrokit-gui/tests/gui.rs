// SPDX-License-Identifier: LGPL-3.0-only
//! Single integration-test binary (one link step instead of one per file).

#[path = "render_gauge_preview.rs"]
mod render_gauge_preview;
#[path = "test_color_emoji_rendering.rs"]
mod test_color_emoji_rendering;
#[path = "test_color_system.rs"]
mod test_color_system;
#[path = "test_directwrite_face.rs"]
mod test_directwrite_face;
#[path = "test_directwrite_faces.rs"]
mod test_directwrite_faces;
#[path = "test_directwrite_fallbacks.rs"]
mod test_directwrite_fallbacks;
#[path = "test_directwrite_families.rs"]
mod test_directwrite_families;
#[path = "test_directwrite_gaps.rs"]
mod test_directwrite_gaps;
#[path = "test_directwrite_modes.rs"]
mod test_directwrite_modes;
#[path = "test_directwrite_placement.rs"]
mod test_directwrite_placement;
#[path = "test_font_fallback_metrics_parity.rs"]
mod test_font_fallback_metrics_parity;
#[path = "test_font_features.rs"]
mod test_font_features;
#[path = "test_font_file_mapping.rs"]
mod test_font_file_mapping;
#[path = "test_font_line_metrics_oracle.rs"]
mod test_font_line_metrics_oracle;
#[path = "test_font_memory_face_cache.rs"]
mod test_font_memory_face_cache;
#[path = "test_font_resource_dedup.rs"]
mod test_font_resource_dedup;
#[path = "test_font_style.rs"]
mod test_font_style;
#[path = "test_ft_golden.rs"]
mod test_ft_golden;
#[path = "test_geometry_system.rs"]
mod test_geometry_system;
#[path = "test_glyph_blit_placement.rs"]
mod test_glyph_blit_placement;
#[path = "test_glyph_cache.rs"]
mod test_glyph_cache;
#[path = "test_gui_p2_hardening.rs"]
mod test_gui_p2_hardening;
#[path = "test_image_p2_qimage_parity.rs"]
mod test_image_p2_qimage_parity;
#[path = "test_image_system.rs"]
mod test_image_system;
#[path = "test_linux_fonts.rs"]
mod test_linux_fonts;
#[path = "test_macos_system_font.rs"]
mod test_macos_system_font;
#[path = "test_outline_face_parity.rs"]
mod test_outline_face_parity;
#[path = "test_p2_geometry_overflow_singular.rs"]
mod test_p2_geometry_overflow_singular;
#[path = "test_painter_advanced_features.rs"]
mod test_painter_advanced_features;
#[path = "test_painter_rotated_clip.rs"]
mod test_painter_rotated_clip;
#[path = "test_qcolor.rs"]
mod test_qcolor;
#[path = "test_qcolor_ops.rs"]
mod test_qcolor_ops;
#[path = "test_qcssparser.rs"]
mod test_qcssparser;
#[path = "test_qpath_stroker.rs"]
mod test_qpath_stroker;
#[path = "test_qregion_shapes.rs"]
mod test_qregion_shapes;
#[path = "test_qt_text_advances.rs"]
mod test_qt_text_advances;
#[path = "test_rich_text_detection.rs"]
mod test_rich_text_detection;
#[path = "test_shaping_face_cache.rs"]
mod test_shaping_face_cache;
#[path = "test_solid_fill_blend.rs"]
mod test_solid_fill_blend;
#[path = "test_text_draw_golden.rs"]
mod test_text_draw_golden;
#[path = "test_text_layout_oracle.rs"]
mod test_text_layout_oracle;
#[path = "test_text_system.rs"]
mod test_text_system;
#[path = "test_text_tabs.rs"]
mod test_text_tabs;
#[path = "test_text_transform_parity.rs"]
mod test_text_transform_parity;
