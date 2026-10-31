// SPDX-License-Identifier: LGPL-3.0-only
//! Residual alignment items (1)-(5) against PySide6 6.11.2 offscreen
//! (one probe line per case; quoted beside
//! each test). Containers are 200x100 with zero margins and spacing; leaves are Fixed 40x20.

use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::application::{GuiApplication, LayoutDirection};
use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

/// Holds the global layout direction for the test and restores left-to-right afterwards.
struct DirectionGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn with_direction(direction: LayoutDirection) -> DirectionGuard {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    GuiApplication::set_layout_direction(direction);
    DirectionGuard { _lock: guard }
}

impl Drop for DirectionGuard {
    fn drop(&mut self) {
        GuiApplication::set_layout_direction(LayoutDirection::LeftToRight);
    }
}

/// A probe widget: `hint`, minimum size hint 0x0, and with `hfw` `heightForWidth(w) = 1600 / w`
/// (the probe's `Hfw` widget).
struct Probe {
    base: WidgetBase,
    hint: Size,
    hfw: bool,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }
}

impl Widget for Probe {
    fn widget_base(&self) -> &WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }
    fn geometry(&self) -> Rect {
        self.base.geometry()
    }
    fn set_geometry(&self, rect: Rect) {
        self.base.set_geometry(rect);
    }
    fn size_hint(&self) -> Size {
        self.hint
    }
    fn minimum_size_hint(&self) -> Size {
        Size::new(0, 0)
    }
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        true
    }
    fn update(&self) {}
    fn dirty_rect(&self) -> Option<Rect> {
        None
    }
    fn clear_dirty(&self) {}
    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        None
    }
    fn set_window_id(&self, _window_id: Option<ObjectId>) {}
    fn children(&self) -> Vec<WidgetRef> {
        Vec::new()
    }
    fn add_child(&mut self, _child: WidgetRef) {}
    fn remove_child(&mut self, _child_id: ObjectId) {}
    fn has_height_for_width(&self) -> bool {
        self.hfw
    }
    fn height_for_width(&self, width: i32) -> i32 {
        if self.hfw {
            1600 / width.max(1)
        } else {
            -1
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn probe(w: i32, h: i32, policy: Policy, hfw: bool) -> WidgetRef {
    let p = Probe {
        base: WidgetBase::new(),
        hint: Size::new(w, h),
        hfw,
    };
    p.base.set_size_policy(QSizePolicy::new(policy, policy));
    Rc::new(RefCell::new(Box::new(p)))
}

fn fixed() -> WidgetRef {
    probe(40, 20, Policy::Fixed, false)
}

fn hfw() -> WidgetRef {
    probe(40, 20, Policy::Preferred, true)
}

fn rect_of(w: &WidgetRef) -> (i32, i32, i32, i32) {
    let g = w.borrow().geometry();
    (g.x, g.y, g.width, g.height)
}

/// `leaf` in the coordinates of the container its nested layout's `holder` sits in.
fn in_container(holder: &WidgetRef, leaf: &WidgetRef) -> (i32, i32, i32, i32) {
    let h = holder.borrow().geometry();
    let (x, y, w, hgt) = rect_of(leaf);
    (h.x + x, h.y + y, w, hgt)
}

fn hbox() -> BoxLayout {
    let mut b = BoxLayout::horizontal();
    b.set_margins(Margins::new(0, 0, 0, 0));
    b.set_spacing(0);
    b
}

fn vbox() -> BoxLayout {
    let mut b = BoxLayout::vertical();
    b.set_margins(Margins::new(0, 0, 0, 0));
    b.set_spacing(0);
    b
}

fn grid() -> GridLayout {
    let mut g = GridLayout::new();
    g.set_margins(Margins::new(0, 0, 0, 0));
    g.set_horizontal_spacing(0);
    g.set_vertical_spacing(0);
    g
}

/// Puts `layout` on a 200x100 container and lays it out.
fn lay_out(layout: Box<dyn Layout>) -> WidgetRef {
    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 200, 100),
    ))));
    container.borrow_mut().set_layout(layout);
    container.borrow().update_layout();
    container
}

fn maximum_of(container: &WidgetRef) -> (i32, i32) {
    let widget = container.borrow();
    let layout = widget.layout_ref_mut().expect("container has a layout");
    let m = layout.maximum_size();
    (m.width, m.height)
}

/// The alignment stored on the nested layout `holder` carries (`QLayout::alignment`).
fn nested_alignment(holder: &WidgetRef) -> u32 {
    let h = holder.borrow();
    let layout = h
        .layout_ref_mut()
        .expect("holder carries the nested layout");
    layout.alignment().bits()
}

// ---------------------------------------------------------------------------------------------
// (1) A vertically aligned item with heightForWidth takes heightForWidth(width)
//     (`QWidgetItem::setGeometry`, qlayoutitem.cpp:441-447).
// ---------------------------------------------------------------------------------------------

#[test]
fn item1_hbox_v_center_hfw_item_takes_its_height_for_its_width() {
    // probe: item1_hbox_vcenter_hfw (0,46,200,8)
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let w = hfw();
    let mut h = hbox();
    h.add_widget_aligned(w.clone(), 0, Alignment::V_CENTER);
    let _c = lay_out(Box::new(h));
    assert_eq!(rect_of(&w), (0, 46, 200, 8));
}

#[test]
fn item1_grid_left_v_center_hfw_item_takes_its_height_for_its_hint_width() {
    // probe: item1_grid_left_vcenter_hfw (0,30,40,40)
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let w = hfw();
    let mut g = grid();
    g.add_widget_aligned(w.clone(), 0, 0, 1, 1, Alignment::LEFT | Alignment::V_CENTER);
    let _c = lay_out(Box::new(g));
    assert_eq!(rect_of(&w), (0, 30, 40, 40));
}

#[test]
fn item1_vbox_top_hfw_item_takes_its_height_for_its_width() {
    // probe: item1_vbox_top_hfw (0,0,200,8)
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let w = hfw();
    let mut v = vbox();
    v.add_widget_aligned(w.clone(), 0, Alignment::TOP);
    let _c = lay_out(Box::new(v));
    assert_eq!(rect_of(&w), (0, 0, 200, 8));
}

// ---------------------------------------------------------------------------------------------
// (2) A layout's own alignment (`QLayout::alignmentRect`, qlayout.cpp:1410-1455; maximum size
//     qboxlayout.cpp:618-623, qgridlayout.cpp:1216-1219).
// ---------------------------------------------------------------------------------------------

#[test]
fn item2_vbox_layout_alignment_right_bottom_places_the_content_and_unbounds_the_maximum() {
    // probe: item2_vbox_layout_right_bottom (160,80,40,20) max=(524287,524287)
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let leaf = fixed();
    let mut v = vbox();
    v.add_widget(leaf.clone());
    v.set_layout_alignment(Alignment::RIGHT | Alignment::BOTTOM);
    let c = lay_out(Box::new(v));
    assert_eq!(rect_of(&leaf), (160, 80, 40, 20));
    assert_eq!(maximum_of(&c), (524287, 524287));
}

#[test]
fn item2_grid_layout_alignment_center_places_the_content_and_unbounds_the_maximum() {
    // probe: item2_grid_layout_center (80,40,40,20) max=(524287,524287)
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let leaf = fixed();
    let mut g = grid();
    g.add_widget(leaf.clone(), 0, 0);
    g.set_layout_alignment(Alignment::CENTER);
    let c = lay_out(Box::new(g));
    assert_eq!(rect_of(&leaf), (80, 40, 40, 20));
    assert_eq!(maximum_of(&c), (524287, 524287));
}

#[test]
fn item2_top_aligned_layout_takes_the_height_for_its_width() {
    // probe: item2_vbox_layout_top_hfw (0,0,200,8)
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let w = hfw();
    let mut v = vbox();
    v.add_widget(w.clone());
    v.set_layout_alignment(Alignment::TOP);
    let _c = lay_out(Box::new(v));
    assert_eq!(rect_of(&w), (0, 0, 200, 8));
}

// ---------------------------------------------------------------------------------------------
// (3) Right-to-left: `QStyle::visualAlignment` flips Left/Right unless `AlignAbsolute`
//     (qguiapplication_p.h:176-186).
// ---------------------------------------------------------------------------------------------

#[test]
fn item3_rtl_layout_alignment_left_is_placed_on_the_right() {
    // probe: item3_rtl_hbox_layout_left (160,40,40,20)
    let _dir = with_direction(LayoutDirection::RightToLeft);
    let leaf = fixed();
    let mut h = hbox();
    h.add_widget(leaf.clone());
    h.set_layout_alignment(Alignment::LEFT);
    let _c = lay_out(Box::new(h));
    assert_eq!(rect_of(&leaf), (160, 40, 40, 20));
}

#[test]
fn item3_rtl_layout_alignment_left_absolute_stays_on_the_left() {
    // probe: item3_rtl_hbox_layout_left_absolute (0,40,40,20)
    let _dir = with_direction(LayoutDirection::RightToLeft);
    let leaf = fixed();
    let mut h = hbox();
    h.add_widget(leaf.clone());
    h.set_layout_alignment(Alignment::LEFT | Alignment::ABSOLUTE);
    let _c = lay_out(Box::new(h));
    assert_eq!(rect_of(&leaf), (0, 40, 40, 20));
}

#[test]
fn item3_rtl_grid_item_right_is_placed_on_the_left() {
    // probe: item3_rtl_grid_item_right_top (0,0,40,20)
    let _dir = with_direction(LayoutDirection::RightToLeft);
    let leaf = fixed();
    let mut g = grid();
    g.add_widget_aligned(leaf.clone(), 0, 0, 1, 1, Alignment::RIGHT | Alignment::TOP);
    let _c = lay_out(Box::new(g));
    assert_eq!(rect_of(&leaf), (0, 0, 40, 20));
}

#[test]
fn item3_rtl_grid_item_right_absolute_stays_on_the_right() {
    // probe: item3_rtl_grid_item_right_absolute_top (160,0,40,20)
    let _dir = with_direction(LayoutDirection::RightToLeft);
    let leaf = fixed();
    let mut g = grid();
    g.add_widget_aligned(
        leaf.clone(),
        0,
        0,
        1,
        1,
        Alignment::RIGHT | Alignment::ABSOLUTE | Alignment::TOP,
    );
    let _c = lay_out(Box::new(g));
    assert_eq!(rect_of(&leaf), (160, 0, 40, 20));
}

// ---------------------------------------------------------------------------------------------
// (4) `QLayout::setAlignment(QLayout*, align)` stores the alignment on the nested layout
//     (qlayout.cpp:213-227); `QGridLayout::addLayout(.., align)` does the same.
// ---------------------------------------------------------------------------------------------

#[test]
fn item4_set_alignment_on_a_nested_box_layout_places_its_leaf() {
    // probe: item4_box_set_alignment_nested ok=True (160,80,40,20) inner_align=66
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let leaf = fixed();
    let mut inner = vbox();
    inner.add_widget(leaf.clone());
    let mut outer = hbox();
    let holder = outer.add_layout(Box::new(inner), 0);
    assert!(outer.set_alignment(&holder, Alignment::RIGHT | Alignment::BOTTOM));
    let _c = lay_out(Box::new(outer));
    assert_eq!(in_container(&holder, &leaf), (160, 80, 40, 20));
    assert_eq!(nested_alignment(&holder), 66);
}

#[test]
fn item4_grid_add_layout_center_places_its_leaf() {
    // probe: item4_grid_add_layout_center (80,40,40,20) inner_align=132
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let leaf = fixed();
    let mut inner = vbox();
    inner.add_widget(leaf.clone());
    let mut g = grid();
    let holder = g.add_layout(Box::new(inner), 0, 0, 1, 1, Alignment::CENTER);
    let _c = lay_out(Box::new(g));
    assert_eq!(in_container(&holder, &leaf), (80, 40, 40, 20));
    assert_eq!(nested_alignment(&holder), 132);
}

// ---------------------------------------------------------------------------------------------
// (5) One `Qt::Alignment` for layout items, labels, progress bars and text blocks; a `QLabel`
//     honours its vertical flags and indents on every aligned side. Labels need the Windows fonts.
// ---------------------------------------------------------------------------------------------

#[cfg(windows)]
mod label {
    use super::*;
    use ferrokit_gui::geometry::primitives::PointF;
    use ferrokit_gui::paint::{PaintDevice, Painter, Pen, Pixmap};
    use ferrokit_gui::text::font::Font;
    use ferrokit_gui::text::font_metrics::FontMetrics;
    use ferrokit_gui::text::{TextBlockFormat, TextDocument};
    use ferrokit_gui::tiny_skia::Color;

    const W: i32 = 200;
    const H: i32 = 60;
    const TEXT: &str = "Hg";

    fn font() -> Font {
        Font::new("Segoe UI", 12.0)
    }

    fn metrics() -> FontMetrics {
        FontMetrics::from_font(&font())
    }

    fn paint_into(w: i32, h: i32, paint: impl FnOnce(&mut Painter<'_>)) -> Pixmap {
        let mut pm = Pixmap::with_dpr(w as u32, h as u32, 1.0).unwrap();
        {
            let mut p = Painter::begin(&mut pm);
            paint(&mut p);
        }
        pm
    }

    /// `TEXT` drawn the way a plain `Label` draws one line, its box at (`x`, `top`), into `w`x`h`.
    fn reference_line(w: i32, h: i32, x: f32, top: f32) -> Pixmap {
        let f = font();
        let ascent = metrics().ascent;
        paint_into(w, h, |p| {
            p.set_pen(Pen::new(Color::BLACK, 1.0));
            p.draw_text(PointF::new(x, top + ascent), TEXT, &f);
        })
    }

    fn label(text: &str, align: Alignment, w: i32, h: i32, qss: Option<&str>) -> Label {
        let mut l = Label::new(text);
        l.set_font(font());
        if let Some(qss) = qss {
            l.set_style_sheet(qss);
        }
        l.set_alignment(align);
        l.set_geometry(Rect::new(0, 0, w, h));
        l
    }

    fn painted(l: &mut Label) -> Pixmap {
        let g = l.geometry();
        paint_into(g.width, g.height, |p| l.paint_event(p))
    }

    /// Bounding box of the painted pixels, `(left, top, right, bottom)` inclusive, as the probe's
    /// `ink()` reports it.
    fn ink(pm: &Pixmap) -> Option<(usize, usize, usize, usize)> {
        let w = pm.width() as usize;
        let mut bbox: Option<(usize, usize, usize, usize)> = None;
        for (i, px) in pm.data().as_chunks::<4>().0.iter().enumerate() {
            if px[3] == 0 {
                continue;
            }
            let (x, y) = (i % w, i / w);
            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x), b.max(y)),
            });
        }
        bbox
    }

    fn assert_same_pixels(got: &Pixmap, want: &Pixmap, what: &str) {
        assert_eq!(ink(got), ink(want), "{what}: ink bounding box");
        assert!(got.data() == want.data(), "{what}: pixels differ");
    }

    #[test]
    fn item5_one_alignment_type_with_qt_values_and_defaults() {
        // probe: item5_types label=AlignmentFlag item=AlignmentFlag block=AlignmentFlag
        // progress=AlignmentFlag; item5_defaults label=129 progress=1 block_unset=1 html_center=4
        // html_plain=1 center=132.
        let _dir = with_direction(LayoutDirection::LeftToRight);
        let a: ferrokit_gui::Alignment = Alignment::RIGHT | Alignment::BOTTOM;
        let mut l = Label::new(TEXT);
        assert_eq!(l.alignment().bits(), 129);
        l.set_alignment(a);
        let mut bar = ProgressBar::new();
        assert_eq!(bar.alignment().bits(), 1);
        bar.set_alignment(a);
        let block = TextBlockFormat {
            alignment: a,
            ..Default::default()
        };
        let mut h = hbox();
        h.add_widget_aligned(fixed(), 0, a);
        h.set_layout_alignment(a);
        assert_eq!(
            (
                l.alignment(),
                bar.alignment(),
                block.alignment,
                h.alignment()
            ),
            (a, a, a, a)
        );

        assert_eq!(TextBlockFormat::default().alignment.bits(), 1);
        let doc = TextDocument::from_html(r#"<p align="center">x</p><p>y</p>"#);
        assert_eq!(doc.block_at(0).unwrap().block_format().alignment.bits(), 4);
        assert_eq!(doc.block_at(1).unwrap().block_format().alignment.bits(), 1);
        assert_eq!(Alignment::CENTER.bits(), 132);
    }

    #[test]
    fn item5_label_aligned_left_only_draws_its_text_at_the_top() {
        // probe: item5_label_ink_left_only (0,2,14,12) == item5_label_ink_left_top (0,2,14,12):
        // no vertical flag puts the text at the top (qt_format_text, qpainter.cpp:7419-7424).
        let _dir = with_direction(LayoutDirection::LeftToRight);
        let got = painted(&mut label(TEXT, Alignment::LEFT, W, H, None));
        assert_same_pixels(
            &got,
            &reference_line(W, H, 0.0, 0.0),
            "AlignLeft draws at the top",
        );
        let top = painted(&mut label(
            TEXT,
            Alignment::LEFT | Alignment::TOP,
            W,
            H,
            None,
        ));
        assert_same_pixels(&top, &got, "AlignLeft|AlignTop");
    }

    #[test]
    fn item5_label_bottom_and_v_center_place_the_text_by_its_height() {
        // probe: item5_label_ink_left_bottom (0,48,14,58), item5_label_ink_left_vcenter (0,25,..),
        // fm_height=14: Bottom puts the text box at h - height, VCenter at (h - height) / 2.
        let _dir = with_direction(LayoutDirection::LeftToRight);
        let th = metrics().height;
        let bottom = painted(&mut label(
            TEXT,
            Alignment::LEFT | Alignment::BOTTOM,
            W,
            H,
            None,
        ));
        assert_same_pixels(
            &bottom,
            &reference_line(W, H, 0.0, H as f32 - th),
            "AlignBottom",
        );
        let centred = painted(&mut label(
            TEXT,
            Alignment::LEFT | Alignment::V_CENTER,
            W,
            H,
            None,
        ));
        let want = reference_line(W, H, 0.0, (H as f32 - th) / 2.0);
        assert_same_pixels(&centred, &want, "AlignVCenter");
        // probe: item5_label_ink_vcenter_only (0,25,14,35): no horizontal flag is AlignLeft.
        let v_only = painted(&mut label(TEXT, Alignment::V_CENTER, W, H, None));
        assert_same_pixels(&v_only, &want, "AlignVCenter alone");
    }

    #[test]
    fn item5_label_right_bottom_places_the_text_in_the_corner() {
        // probe: item5_label_ink_right_bottom (184,48,198,58).
        let _dir = with_direction(LayoutDirection::LeftToRight);
        let (f, m) = (font(), metrics());
        let advance = m.horizontal_advance_exact(TEXT, &f);
        let got = painted(&mut label(
            TEXT,
            Alignment::RIGHT | Alignment::BOTTOM,
            W,
            H,
            None,
        ));
        let want = reference_line(W, H, W as f32 - advance, H as f32 - m.height);
        assert_same_pixels(&got, &want, "AlignRight|AlignBottom");
    }

    #[test]
    fn item5_framed_label_adds_the_indent_on_its_top_and_bottom() {
        // probe (1px border): item5_framed_hint_left_vcenter (24,16), hcenter_vcenter (18,16),
        // hcenter_top (18,22), left_top (24,22), hcenter_bottom (18,22), x_adv=6: the 'x' advance
        // is added on each aligned side (qlabel.cpp:558-568).
        let _dir = with_direction(LayoutDirection::LeftToRight);
        let qss = Some("border: 1px solid black;");
        let hint = |align: Alignment| {
            let s = label(TEXT, align, W, H, qss).size_hint();
            (s.width, s.height)
        };
        let (lw, lh) = hint(Alignment::LEFT | Alignment::V_CENTER);
        let (cw, ch) = hint(Alignment::H_CENTER | Alignment::V_CENTER);
        let indent = lw - cw;
        assert!(indent > 0, "a framed left-aligned label is indented");
        assert_eq!(lh, ch);
        assert_eq!(
            hint(Alignment::H_CENTER | Alignment::TOP),
            (cw, ch + indent)
        );
        assert_eq!(hint(Alignment::LEFT | Alignment::TOP), (lw, ch + indent));
        assert_eq!(
            hint(Alignment::H_CENTER | Alignment::BOTTOM),
            (cw, ch + indent)
        );
    }

    #[test]
    fn item5_framed_label_insets_its_text_by_half_the_indent_on_the_aligned_sides() {
        // probe (1px transparent border): item5_framed_ink_left_top (4,6,18,16) — 1 + 6/2 from the
        // left and the top; item5_framed_ink_left_vcenter (4,25,..) — no vertical inset;
        // item5_framed_ink_right_bottom (180,44,194,54) (qlabel.cpp:1474-1488).
        let _dir = with_direction(LayoutDirection::LeftToRight);
        let (f, m) = (font(), metrics());
        let qss = Some("border: 1px solid transparent;");
        let half = (m.horizontal_advance_exact("x", &f).round() / 2.0).floor();
        let inset = 1.0 + half;
        let top_left = painted(&mut label(
            TEXT,
            Alignment::LEFT | Alignment::TOP,
            W,
            H,
            qss,
        ));
        assert_same_pixels(
            &top_left,
            &reference_line(W, H, inset, inset),
            "framed AlignLeft|AlignTop",
        );
        let centred = painted(&mut label(
            TEXT,
            Alignment::LEFT | Alignment::V_CENTER,
            W,
            H,
            qss,
        ));
        let want = reference_line(W, H, inset, 1.0 + (H as f32 - 2.0 - m.height) / 2.0);
        assert_same_pixels(&centred, &want, "framed AlignLeft|AlignVCenter");
        let advance = m.horizontal_advance_exact(TEXT, &f);
        let corner = painted(&mut label(
            TEXT,
            Alignment::RIGHT | Alignment::BOTTOM,
            W,
            H,
            qss,
        ));
        let right = W as f32 - inset - advance;
        let want = reference_line(W, H, right, H as f32 - inset - m.height);
        assert_same_pixels(&corner, &want, "framed AlignRight|AlignBottom");
    }

    #[test]
    fn item5_rich_label_taller_than_its_rect_never_starts_above_it() {
        // probe (200x8): item5_short_label_ink_rich_vcenter == rich_bottom == rich_top (0,2,15,7):
        // `QLabelPrivate::layoutRect` clamps the offset at 0 (qlabel.cpp:1632-1635).
        let _dir = with_direction(LayoutDirection::LeftToRight);
        let rich = |align: Alignment| {
            let mut l = label("<b>Hg</b>", align, W, 8, None);
            l.set_rich(true);
            painted(&mut l)
        };
        let top = rich(Alignment::LEFT | Alignment::TOP);
        assert_same_pixels(
            &rich(Alignment::LEFT | Alignment::V_CENTER),
            &top,
            "rich AlignVCenter",
        );
        assert_same_pixels(
            &rich(Alignment::LEFT | Alignment::BOTTOM),
            &top,
            "rich AlignBottom",
        );
    }

    #[test]
    fn item5_plain_label_taller_than_its_rect_is_centred_unclamped() {
        // probe (200x8): item5_short_label_ink_plain_vcenter (0,0,14,7) vs plain_top (0,2,14,7):
        // plain text is offset by (h - height) / 2 (qpainter.cpp:7421-7424).
        let _dir = with_direction(LayoutDirection::LeftToRight);
        let got = painted(&mut label(
            TEXT,
            Alignment::LEFT | Alignment::V_CENTER,
            W,
            8,
            None,
        ));
        let want = reference_line(W, 8, 0.0, (8.0 - metrics().height) / 2.0);
        assert_same_pixels(&got, &want, "short plain AlignVCenter");
    }

    #[test]
    fn item3_rtl_label_keeps_latin_text_on_the_left() {
        // probe: item3_rtl_label_left_ink (0,15,14,25) layoutDirection=1: a label aligns by its
        // text's direction (qlabel.cpp:996, 1456-1464), left-to-right for Latin text.
        let ltr = {
            let _dir = with_direction(LayoutDirection::LeftToRight);
            painted(&mut label(
                TEXT,
                Alignment::LEFT | Alignment::V_CENTER,
                W,
                40,
                None,
            ))
        };
        let _dir = with_direction(LayoutDirection::RightToLeft);
        let rtl = painted(&mut label(
            TEXT,
            Alignment::LEFT | Alignment::V_CENTER,
            W,
            40,
            None,
        ));
        assert_same_pixels(&rtl, &ltr, "RTL AlignLeft label");
    }
}
