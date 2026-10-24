// SPDX-License-Identifier: LGPL-3.0-only
//! Framed container widget (`QFrame`) plus the reusable frame geometry/painting
//! model shared by framed widgets such as `LCDNumber`.

use ferrokit_core::event::{Event, EventKind, FocusReason};
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Rect, RectF, Size};
use ferrokit_gui::paint::palette::{ColorGroup, ColorRole, Palette};
use ferrokit_gui::paint::Painter;
use ferrokit_gui::tiny_skia::Color;

use crate::focus::FocusPolicy;
use crate::layout::Layout;
use crate::layout_engine::{owner_layout_margin, WIDGET_LAYOUT_SPACING};
use crate::size_policy::{Policy, QSizePolicy};
use crate::widget::{Widget, WidgetBase, WidgetRef, WidgetWeak};

/// Frame shape (`QFrame::Shape`). Discriminants match Qt's `frameStyle` bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum FrameShape {
    /// No frame is drawn.
    #[default]
    NoFrame = 0,
    /// A box around the contents.
    Box = 1,
    /// A panel making the contents appear raised or sunken.
    Panel = 2,
    /// A two-pixel Windows-style panel.
    WinPanel = 3,
    /// A horizontal separator line.
    HLine = 4,
    /// A vertical separator line.
    VLine = 5,
    /// A style-dependent panel (two pixels wide).
    StyledPanel = 6,
}

/// Frame 3D shadow (`QFrame::Shadow`). Discriminants match Qt's `frameStyle` bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum FrameShadow {
    /// Drawn in the foreground color without 3D effect.
    #[default]
    Plain = 0x10,
    /// Appears raised using light/dark shading.
    Raised = 0x20,
    /// Appears sunken using light/dark shading.
    Sunken = 0x30,
}

/// Mask selecting the shape bits of a combined frame style (`QFrame::Shape_Mask`).
pub const FRAME_SHAPE_MASK: i32 = 0x000f;
/// Mask selecting the shadow bits of a combined frame style (`QFrame::Shadow_Mask`).
pub const FRAME_SHADOW_MASK: i32 = 0x00f0;

/// Default style frame width used by `StyledPanel` (`QStyle::PM_DefaultFrameWidth`).
pub const DEFAULT_STYLED_FRAME_WIDTH: i32 = 2;

impl FrameShape {
    fn from_bits(bits: i32) -> Self {
        match bits & FRAME_SHAPE_MASK {
            1 => Self::Box,
            2 => Self::Panel,
            3 => Self::WinPanel,
            4 => Self::HLine,
            5 => Self::VLine,
            6 => Self::StyledPanel,
            _ => Self::NoFrame,
        }
    }
}

impl FrameShadow {
    fn from_bits(bits: i32) -> Self {
        match bits & FRAME_SHADOW_MASK {
            0x20 => Self::Raised,
            0x30 => Self::Sunken,
            _ => Self::Plain,
        }
    }
}

/// Frame parameters (`QFrame` shape/shadow/lineWidth/midLineWidth) with Qt's
/// frame-width rules and painting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameStyle {
    pub shape: FrameShape,
    pub shadow: FrameShadow,
    pub line_width: i32,
    pub mid_line_width: i32,
}

impl Default for FrameStyle {
    fn default() -> Self {
        Self::new(FrameShape::NoFrame, FrameShadow::Plain)
    }
}

impl FrameStyle {
    /// Creates a style with Qt's default line width (1) and mid-line width (0).
    pub const fn new(shape: FrameShape, shadow: FrameShadow) -> Self {
        Self {
            shape,
            shadow,
            line_width: 1,
            mid_line_width: 0,
        }
    }

    /// Combined `shape | shadow` bits (`QFrame::frameStyle`).
    pub fn bits(&self) -> i32 {
        self.shape as i32 | self.shadow as i32
    }

    /// Replaces shape and shadow from combined bits (`QFrame::setFrameStyle`).
    pub fn set_bits(&mut self, bits: i32) {
        self.shape = FrameShape::from_bits(bits);
        self.shadow = FrameShadow::from_bits(bits);
    }

    /// Width of the drawn frame on each side (`QFrame::frameWidth`), following
    /// `QCommonStyle::SE_ShapedFrameContents`.
    pub fn frame_width(&self) -> i32 {
        let lw = self.line_width.max(0);
        let mlw = self.mid_line_width.max(0);
        match self.shape {
            FrameShape::NoFrame => 0,
            FrameShape::Box | FrameShape::HLine | FrameShape::VLine => match self.shadow {
                FrameShadow::Plain => lw,
                FrameShadow::Raised | FrameShadow::Sunken => {
                    lw.saturating_mul(2).saturating_add(mlw)
                }
            },
            FrameShape::Panel => lw,
            FrameShape::WinPanel => 2,
            FrameShape::StyledPanel => DEFAULT_STYLED_FRAME_WIDTH,
        }
    }

    /// The area inside the frame for a given frame rectangle (`QFrame::contentsRect`).
    pub fn contents_rect(&self, frame_rect: Rect) -> Rect {
        let fw = self.frame_width();
        Rect::new(
            frame_rect.x.saturating_add(fw),
            frame_rect.y.saturating_add(fw),
            frame_rect.width.saturating_sub(fw.saturating_mul(2)).max(0),
            frame_rect.height.saturating_sub(fw.saturating_mul(2)).max(0),
        )
    }

    /// Thickness of a separator line drawn for `HLine`/`VLine`.
    fn line_thickness(&self) -> i32 {
        let lw = self.line_width.max(0);
        match self.shadow {
            FrameShadow::Plain => lw,
            FrameShadow::Raised | FrameShadow::Sunken => lw
                .saturating_mul(2)
                .saturating_add(self.mid_line_width.max(0)),
        }
    }

    /// Paints the frame into `rect` (`QFrame::drawFrame`), using the palette's
    /// light/midlight/mid/dark/shadow roles for 3D shading.
    pub fn paint(&self, painter: &mut Painter, rect: Rect, palette: &Palette, enabled: bool) {
        if rect.width <= 0 || rect.height <= 0 {
            return;
        }
        let group = if enabled {
            ColorGroup::Active
        } else {
            ColorGroup::Disabled
        };
        let role = |r: ColorRole| palette.color(group, r);
        let lw = self.line_width.max(0);
        let mlw = self.mid_line_width.max(0);
        // A ring whose inset reaches half the rect paints nothing, so a huge line width never
        // needs more rings than this.
        let rings = rect.width.min(rect.height).saturating_add(1) / 2;
        let sunken = self.shadow == FrameShadow::Sunken;
        let (top_left, bottom_right) = if sunken {
            (role(ColorRole::Dark), role(ColorRole::Light))
        } else {
            (role(ColorRole::Light), role(ColorRole::Dark))
        };
        let foreground = role(ColorRole::WindowText);

        match self.shape {
            FrameShape::NoFrame => {}
            FrameShape::Box => match self.shadow {
                FrameShadow::Plain => {
                    for i in 0..lw.min(rings) {
                        draw_ring(painter, rect, i, foreground, foreground);
                    }
                }
                FrameShadow::Raised | FrameShadow::Sunken => {
                    // qDrawShadeRect: outer bevel, mid band, reversed inner bevel (etched box).
                    for i in 0..lw.min(rings) {
                        draw_ring(painter, rect, i, top_left, bottom_right);
                    }
                    let mid = role(ColorRole::Mid);
                    for i in lw.min(rings)..lw.saturating_add(mlw).min(rings) {
                        draw_ring(painter, rect, i, mid, mid);
                    }
                    for i in lw.saturating_add(mlw).min(rings)
                        ..lw.saturating_mul(2).saturating_add(mlw).min(rings)
                    {
                        draw_ring(painter, rect, i, bottom_right, top_left);
                    }
                }
            },
            FrameShape::Panel => {
                // qDrawShadePanel / qDrawPlainRect.
                let (tl, br) = if self.shadow == FrameShadow::Plain {
                    (foreground, foreground)
                } else {
                    (top_left, bottom_right)
                };
                for i in 0..lw.min(rings) {
                    draw_ring(painter, rect, i, tl, br);
                }
            }
            FrameShape::WinPanel => {
                // qDrawWinPanel: two rings with shadow/midlight accents.
                match self.shadow {
                    FrameShadow::Plain => {
                        draw_ring(painter, rect, 0, foreground, foreground);
                        draw_ring(painter, rect, 1, foreground, foreground);
                    }
                    FrameShadow::Raised => {
                        draw_ring(
                            painter,
                            rect,
                            0,
                            role(ColorRole::Light),
                            role(ColorRole::Shadow),
                        );
                        draw_ring(
                            painter,
                            rect,
                            1,
                            role(ColorRole::Midlight),
                            role(ColorRole::Dark),
                        );
                    }
                    FrameShadow::Sunken => {
                        draw_ring(
                            painter,
                            rect,
                            0,
                            role(ColorRole::Dark),
                            role(ColorRole::Light),
                        );
                        draw_ring(
                            painter,
                            rect,
                            1,
                            role(ColorRole::Shadow),
                            role(ColorRole::Midlight),
                        );
                    }
                }
            }
            FrameShape::StyledPanel => {
                let border = role(ColorRole::Mid);
                draw_ring(painter, rect, 0, border, border);
                match self.shadow {
                    FrameShadow::Plain => {}
                    FrameShadow::Raised => draw_ring(
                        painter,
                        rect,
                        1,
                        role(ColorRole::Light),
                        role(ColorRole::Midlight),
                    ),
                    FrameShadow::Sunken => draw_ring(
                        painter,
                        rect,
                        1,
                        role(ColorRole::Midlight),
                        role(ColorRole::Light),
                    ),
                }
            }
            FrameShape::HLine | FrameShape::VLine => {
                let horizontal = self.shape == FrameShape::HLine;
                let thickness = self.line_thickness();
                let span = if horizontal { rect.height } else { rect.width };
                let start = (span - thickness) / 2;
                // Bands from top (or left) to bottom (or right).
                let bands: [(i32, Color); 3] = if self.shadow == FrameShadow::Plain {
                    [(lw, foreground), (0, foreground), (0, foreground)]
                } else {
                    [
                        (lw, top_left),
                        (mlw, role(ColorRole::Mid)),
                        (lw, bottom_right),
                    ]
                };
                let mut offset = start;
                for (size, color) in bands {
                    if size <= 0 {
                        continue;
                    }
                    let band = if horizontal {
                        RectF::new(
                            rect.x as f32,
                            rect.y.saturating_add(offset) as f32,
                            rect.width as f32,
                            size as f32,
                        )
                    } else {
                        RectF::new(
                            rect.x.saturating_add(offset) as f32,
                            rect.y as f32,
                            size as f32,
                            rect.height as f32,
                        )
                    };
                    painter.fill_rect(band, color);
                    offset = offset.saturating_add(size);
                }
            }
        }
    }
}

/// Draws a one-pixel bevel ring `inset` pixels inside `rect`: top/left edges in
/// `top_left`, bottom/right edges in `bottom_right`.
fn draw_ring(painter: &mut Painter, rect: Rect, inset: i32, top_left: Color, bottom_right: Color) {
    let x = rect.x + inset;
    let y = rect.y + inset;
    let w = rect.width - 2 * inset;
    let h = rect.height - 2 * inset;
    if w <= 0 || h <= 0 {
        return;
    }
    let (xf, yf, wf, hf) = (x as f32, y as f32, w as f32, h as f32);
    painter.fill_rect(RectF::new(xf, yf, wf, 1.0), top_left);
    painter.fill_rect(RectF::new(xf, yf, 1.0, hf), top_left);
    painter.fill_rect(RectF::new(xf, yf + hf - 1.0, wf, 1.0), bottom_right);
    painter.fill_rect(RectF::new(xf + wf - 1.0, yf, 1.0, hf), bottom_right);
}

/// Container widget with an optional frame (`QFrame`).
pub struct Frame {
    pub base: WidgetBase,
    style: FrameStyle,
    /// Explicit frame rectangle; `None` means the whole widget (`QFrame::frameRect`).
    frame_rect: Option<Rect>,
    palette: Palette,
    background: Option<Color>,
}

/// Canonical Qt alias.
pub type QFrame = Frame;

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

impl Frame {
    /// Creates a frame with `NoFrame | Plain`, line width 1 and mid-line width 0.
    pub fn new() -> Self {
        Self::with_style(FrameShape::NoFrame, FrameShadow::Plain)
    }

    /// Creates a frame with the given shape and shadow.
    pub fn with_style(shape: FrameShape, shadow: FrameShadow) -> Self {
        let mut frame = Self {
            base: WidgetBase::new(),
            style: FrameStyle::new(FrameShape::NoFrame, FrameShadow::Plain),
            frame_rect: None,
            palette: Palette::light(),
            background: None,
        };
        frame.set_frame_style(shape as i32 | shadow as i32);
        frame
    }

    pub fn frame_shape(&self) -> FrameShape {
        self.style.shape
    }

    pub fn set_frame_shape(&mut self, shape: FrameShape) {
        self.set_frame_style(shape as i32 | self.style.shadow as i32);
    }

    pub fn frame_shadow(&self) -> FrameShadow {
        self.style.shadow
    }

    pub fn set_frame_shadow(&mut self, shadow: FrameShadow) {
        self.set_frame_style(self.style.shape as i32 | shadow as i32);
    }

    /// Combined `shape | shadow` bits (`QFrame::frameStyle`).
    pub fn frame_style(&self) -> i32 {
        self.style.bits()
    }

    /// Sets shape and shadow from combined bits; separator lines get line size policies.
    pub fn set_frame_style(&mut self, style: i32) {
        self.style.set_bits(style);
        self.base.set_size_policy(match self.style.shape {
            FrameShape::HLine => QSizePolicy::new(Policy::Minimum, Policy::Fixed),
            FrameShape::VLine => QSizePolicy::new(Policy::Fixed, Policy::Minimum),
            _ => QSizePolicy::new(Policy::Preferred, Policy::Preferred),
        });
        self.relayout();
    }

    /// Frame parameters as a value type.
    pub fn frame_params(&self) -> FrameStyle {
        self.style
    }

    pub fn line_width(&self) -> i32 {
        self.style.line_width
    }

    /// Sets the line width; negative values clamp to 0.
    pub fn set_line_width(&mut self, width: i32) {
        self.style.line_width = width.max(0);
        self.relayout();
    }

    pub fn mid_line_width(&self) -> i32 {
        self.style.mid_line_width
    }

    /// Sets the mid-line width; negative values clamp to 0.
    pub fn set_mid_line_width(&mut self, width: i32) {
        self.style.mid_line_width = width.max(0);
        self.relayout();
    }

    /// Width of the frame on each side (`QFrame::frameWidth`).
    pub fn frame_width(&self) -> i32 {
        self.style.frame_width()
    }

    /// Rectangle the frame is drawn in (`QFrame::frameRect`), in local coordinates.
    pub fn frame_rect(&self) -> Rect {
        self.frame_rect
            .unwrap_or_else(|| Rect::new(0, 0, self.base.geometry().width, self.base.geometry().height))
    }

    /// Overrides the frame rectangle; `None` restores the whole-widget default.
    pub fn set_frame_rect(&mut self, rect: Option<Rect>) {
        self.frame_rect = rect;
        self.relayout();
    }

    /// Area inside the frame (`QWidget::contentsRect` for frames).
    pub fn contents_rect(&self) -> Rect {
        self.style.contents_rect(self.frame_rect())
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
        self.update();
    }

    /// Fills the contents area with `color` before drawing the frame (`autoFillBackground`).
    pub fn set_background_color(&mut self, color: Option<Color>) {
        self.background = color;
        self.update();
    }

    fn relayout(&self) {
        let contents = self.contents_rect();
        if let Some(layout) = self.base.layout.borrow_mut().as_mut() {
            layout.set_geometry(contents);
        }
        self.update();
    }
    pub fn resolved_style(&self) -> crate::style::stylesheet::ResolvedStyle {
        let mut buf = [""; 4];
        let props = self.base.properties.borrow();
        let ctx = crate::style::stylesheet::WidgetStyleContext {
            type_name: "QFrame",
            object_name: self.base.object_data.object_name().unwrap_or(""),
            pseudo_states: self.base.style_pseudo_states(false, false, &mut buf),
            sub_control: None,
            attributes: props.as_slice(),
        };
        self.base.resolve_style(&ctx)
    }
}

impl QObject for Frame {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::Resize {
                width,
                height,
                old_width,
                old_height,
            } => {
                self.resize_event(
                    Size::new(*width, *height),
                    Size::new(*old_width, *old_height),
                );
                true
            }
            EventKind::FocusIn { reason } => {
                self.focus_in_event(*reason);
                true
            }
            EventKind::FocusOut { reason } => {
                self.focus_out_event(*reason);
                true
            }
            _ => false,
        }
    }
}

impl Widget for Frame {
    fn widget_base(&self) -> &crate::widget::WidgetBase {
        &self.base
    }

    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }

    fn geometry(&self) -> Rect {
        self.base.geometry()
    }

    fn set_geometry(&self, rect: Rect) {
        if self.base.geometry() != rect {
            let old = Size::new(self.base.geometry().width, self.base.geometry().height);
            self.base.set_geometry(rect);
            self.resize_event(Size::new(rect.width, rect.height), old);
            self.relayout();
        }
    }

    fn size_hint(&self) -> Size {
        let fw = self.frame_width();
        match self.style.shape {
            FrameShape::HLine => Size::new(-1, self.style.line_thickness().max(3)),
            FrameShape::VLine => Size::new(self.style.line_thickness().max(3), -1),
            _ => match self.base.layout.borrow().as_ref() {
                Some(layout) => {
                    let hint = layout.size_hint();
                    Size::new(
                        hint.width.saturating_add(fw.saturating_mul(2)),
                        hint.height.saturating_add(fw.saturating_mul(2)),
                    )
                }
                None => Size::new(-1, -1),
            },
        }
    }
    fn minimum_size(&self) -> Size {
        let style = self.resolved_style();
        let (w, h) = style.min_box_size();
        Size::new(w, h)
    }

    fn maximum_size(&self) -> Size {
        let style = self.resolved_style();
        let (w, h) = style.max_box_size();
        Size::new(w, h)
    }

    fn minimum_size_hint(&self) -> Size {
        let fw = self.frame_width();
        match self.style.shape {
            FrameShape::HLine | FrameShape::VLine => Size::new(-1, -1),
            _ => match self.base.layout.borrow().as_ref() {
                Some(layout) => {
                    let min = layout.minimum_size();
                    Size::new(
                        min.width.saturating_add(fw.saturating_mul(2)),
                        min.height.saturating_add(fw.saturating_mul(2)),
                    )
                }
                None => Size::new(-1, -1),
            },
        }
    }

    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }

    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }

    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }

    fn update(&self) {
        self.base.update();
    }

    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }

    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }

    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        self.base.layout.get_mut().as_mut()
    }

    fn layout_ref_mut(&self) -> Option<std::cell::RefMut<'_, Box<dyn Layout>>> {
        let borrow = self.base.layout.borrow_mut();
        if borrow.is_some() {
            Some(std::cell::RefMut::map(borrow, |opt| opt.as_mut().unwrap()))
        } else {
            None
        }
    }

    fn set_layout(&mut self, mut layout: Box<dyn Layout>) {
        layout.set_default_margin(owner_layout_margin(self.base.parent_widget().is_some()));
        layout.set_geometry(self.contents_rect());
        layout.set_smart_spacing(WIDGET_LAYOUT_SPACING);
        *self.base.layout.get_mut() = Some(layout);
        self.update();
    }

    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }

    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        let has_parent = parent.is_some();
        self.base.set_parent_widget(parent);
        // The owner margin depends on whether this frame is a window (`QLayoutPrivate::getMargin`).
        if let Some(mut layout) = self.layout_ref_mut() {
            layout.set_default_margin(owner_layout_margin(has_parent));
        }
    }

    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }

    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
        for child in self.children() {
            child.borrow().set_window_id(window_id);
        }
    }

    fn children(&self) -> Vec<WidgetRef> {
        self.base.children()
    }

    fn add_child(&mut self, child: WidgetRef) {
        self.base.add_child(child);
        self.update();
    }

    fn remove_child(&mut self, child_id: ObjectId) {
        self.base.remove_child(child_id);
        self.update();
    }

    fn focus_policy(&self) -> FocusPolicy {
        self.base.focus_policy()
    }

    fn set_focus_policy(&self, policy: FocusPolicy) {
        self.base.set_focus_policy(policy);
    }

    fn has_focus(&self) -> bool {
        self.base.has_focus()
    }

    fn set_has_focus(&self, focus: bool) {
        self.base.set_has_focus(focus);
    }

    fn focus_in_event(&mut self, _reason: FocusReason) {
        self.update();
    }

    fn focus_out_event(&mut self, _reason: FocusReason) {
        self.update();
    }

    fn paint_event(&mut self, painter: &mut Painter) {
        let style = self.resolved_style();
        let bg = style.background_color.or(self.background);
        if let Some(color) = bg {
            let c = self.base.geometry();
            painter.fill_rect(
                RectF::new(0.0, 0.0, c.width as f32, c.height as f32),
                color,
            );
        }
        let no_border = style.border_width == Some(0.0)
            || style.border_style.as_deref() == Some("none");
        if !no_border {
            self.style
                .paint(painter, self.frame_rect(), &self.palette, self.base.is_enabled());
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
