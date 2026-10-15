// SPDX-License-Identifier: LGPL-3.0-only
use crate::accessibility::{AccessibleAction, AccessibleRole};
use crate::action::{Action, ActionRef};
use crate::layout::Layout;
use crate::style::{ButtonStyleOption, DefaultStyle, Style};
use crate::widget::{Widget, WidgetBase, WidgetRef, WidgetWeak};
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::notify_borrowed;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::{Point, Rect, RectF, Size};
use ferrokit_gui::paint::Painter;
use ferrokit_gui::text::{Font, FontMetrics};
use ferrokit_gui::tiny_skia::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonState {
    #[default]
    Normal,
    Hovered,
    Pressed,
}

pub struct Button {
    pub base: WidgetBase,
    text: String,
    font: Font,
    state: ButtonState,
    pub clicked: Signal<()>,
    normal_bg: Color,
    hover_bg: Color,
    press_bg: Color,
    text_color: Color,
    border_color: Color,
    border_radius: f32,
    action: Option<ActionRef>,
    style: std::rc::Rc<dyn Style>,
}

impl Button {
    pub fn new(text: impl Into<String>) -> Self {
        let text_str = text.into();
        let font = Font::new("Segoe UI", 13.0);
        let base = WidgetBase::new();
        base.set_focus_policy(crate::focus::FocusPolicy::StrongFocus);
        base.set_size_policy(crate::size_policy::QSizePolicy::new(
            crate::size_policy::Policy::Minimum,
            crate::size_policy::Policy::Fixed,
        ));
        let metrics = FontMetrics::from_font(&font);
        let text_w = metrics.horizontal_advance(&text_str, &font).ceil() as i32 + 16;
        let text_h = metrics.height.ceil() as i32 + 8;
        let char_count = text_str.chars().count();
        let (min_w, min_h) = if char_count <= 2 {
            (20, 18)
        } else if char_count <= 4 {
            (40, 22)
        } else {
            (75, 26)
        };
        base.set_geometry(Rect::new(0, 0, text_w.max(min_w), text_h.max(min_h)));
        let clicked = Signal::with_owner(&base.object_data);
        Self {
            base,
            text: text_str,
            font,
            state: ButtonState::Normal,
            clicked,
            normal_bg: Color::from_rgba8(240, 240, 240, 255),
            hover_bg: Color::from_rgba8(225, 235, 245, 255),
            press_bg: Color::from_rgba8(200, 220, 240, 255),
            text_color: Color::from_rgba8(20, 20, 20, 255),
            border_color: Color::from_rgba8(200, 200, 200, 255),
            border_radius: 4.0,
            action: None,
            style: std::rc::Rc::new(DefaultStyle),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        // QAbstractButton::setText returns early when the text is unchanged (qabstractbutton.cpp:496-497).
        if text == self.text {
            return;
        }
        self.text = text;
        self.update_geometry();
        self.update();
    }

    pub fn set_font(&mut self, font: Font) {
        self.font = font;
        // FontChange (qwidget.cpp:4894-4895) precedes the update work (qwidget.cpp:9502).
        notify_borrowed(self, &mut Event::new(EventKind::FontChange));
        self.update_geometry();
        self.update();
    }

    pub fn state(&self) -> ButtonState {
        self.state
    }

    pub fn set_colors(&mut self, normal: Color, hover: Color, pressed: Color) {
        self.normal_bg = normal;
        self.hover_bg = hover;
        self.press_bg = pressed;
        self.update();
    }

    pub fn set_border_radius(&mut self, radius: f32) {
        self.border_radius = radius;
        self.update();
    }

    pub fn set_border_color(&mut self, color: Color) {
        self.border_color = color;
        self.update();
    }

    pub fn set_text_color(&mut self, color: Color) {
        self.text_color = color;
        self.update();
    }

    /// Shares this button with menus and tool bars; activating it triggers that action.
    pub fn set_action(&mut self, action: Option<ActionRef>) {
        if let Some(action) = &action {
            self.text = action.borrow().display_text();
        }
        self.action = action;
        self.update_geometry();
        self.update();
    }
    pub fn resolved_style(&self) -> crate::style::stylesheet::ResolvedStyle {
        let mut buf = [""; 4];
        let props = self.base.properties.borrow();
        let pseudo = self.base.style_pseudo_states(
            self.state == ButtonState::Hovered,
            self.state == ButtonState::Pressed,
            &mut buf,
        );
        let ctx = crate::style::stylesheet::WidgetStyleContext {
            type_name: "QPushButton",
            object_name: self.base.object_data.object_name.as_deref().unwrap_or(""),
            pseudo_states: pseudo,
            sub_control: None,
            attributes: props.as_slice(),
        };
        self.base.resolve_style(&ctx)
    }

    /// The button's font with the stylesheet's size, family, weight and letter spacing applied.
    fn styled_font(&self, style: &crate::style::stylesheet::ResolvedStyle) -> Font {
        let mut font = self.font.clone();
        if let Some(sz) = style.font_size {
            font.size = sz.round();
        }
        if let Some(fam) = style.font_family() {
            font.family = fam;
        }
        if let Some(w) = style.font_weight {
            font.weight = match w {
                851..=1000 => ferrokit_gui::text::FontWeight::Black,
                751..=850 => ferrokit_gui::text::FontWeight::ExtraBold,
                651..=750 => ferrokit_gui::text::FontWeight::Bold,
                551..=650 => ferrokit_gui::text::FontWeight::SemiBold,
                451..=550 => ferrokit_gui::text::FontWeight::Medium,
                251..=350 => ferrokit_gui::text::FontWeight::Light,
                151..=250 => ferrokit_gui::text::FontWeight::ExtraLight,
                0..=150 => ferrokit_gui::text::FontWeight::Thin,
                _ => ferrokit_gui::text::FontWeight::Normal,
            };
        }
        if let Some(spacing) = style.letter_spacing {
            font.letter_spacing = spacing;
        }
        font
    }

    pub fn action(&self) -> Option<ActionRef> {
        self.action.clone()
    }

    /// Replaces the rendering policy used by this button.
    pub fn set_style(&mut self, style: std::rc::Rc<dyn Style>) {
        self.style = style;
        self.update();
    }

    pub fn style(&self) -> &dyn Style {
        self.style.as_ref()
    }

    fn activate(&self) {
        if let Some(action) = &self.action {
            if !action.borrow().is_enabled() {
                return;
            }
            Action::trigger(action);
        }
        self.clicked.emit(&());
    }
}

impl QObject for Button {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }

    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.base.object_data
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::Enter { .. } => {
                self.enter_event(Point::new(0, 0));
                true
            }
            EventKind::Leave => {
                self.leave_event();
                true
            }
            EventKind::MouseButtonPress { x, y, button } => {
                self.mouse_press_event(Point::new(*x, *y), *button, 0);
                // QAbstractButton::mousePressEvent ignores every button but the left one.
                if *button != 1 {
                    event.ignore();
                }
                true
            }
            EventKind::MouseButtonRelease { x, y, button } => {
                self.mouse_release_event(Point::new(*x, *y), *button, 0);
                if *button != 1 {
                    event.ignore();
                }
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
            EventKind::KeyPress {
                key,
                modifiers,
                is_repeat,
            } => {
                self.key_press_event(*key, *modifiers, *is_repeat);
                true
            }
            EventKind::KeyRelease { key, modifiers } => {
                self.key_release_event(*key, *modifiers);
                true
            }
            _ => false,
        }
    }
}

impl Widget for Button {
    fn widget_base(&self) -> &crate::widget::WidgetBase {
        &self.base
    }

    fn id(&self) -> ObjectId {
        self.base.object_data.id
    }

    fn geometry(&self) -> Rect {
        self.base.geometry()
    }

    fn set_geometry(&self, rect: Rect) {
        self.base.set_geometry(rect);
    }
    fn size_hint(&self) -> Size {
        let style = self.resolved_style();
        let font = self.styled_font(&style);
        let metrics = FontMetrics::from_font(&font);
        let text_w = metrics.horizontal_advance_exact(&self.text, &font).ceil() as i32;
        let text_h = metrics.height.ceil() as i32;

        let [pt, pr, pb, pl] = style.padding.unwrap_or([0.0; 4]);
        let border = style.border_width.unwrap_or(0.0).max(0.0);
        let pad_h = (pl + pr).round() as i32;
        let pad_v = (pt + pb).round() as i32;
        let border_h = (border * 2.0).round() as i32;
        let border_v = (border * 2.0).round() as i32;

        if style.padding.is_none() && style.border_width.is_none() && style.min_width.is_none() && style.max_height.is_none() && style.width.is_none() && style.height.is_none() {
            let style_metrics = self.style.metrics();
            let styled = self.style.size_from_contents(
                Size::new(text_w, text_h),
                style_metrics.button_horizontal_padding,
                style_metrics.button_vertical_padding,
            );
            let char_count = self.text.chars().count();
            let (min_w, min_h) = if char_count <= 2 {
                (20, 18)
            } else if char_count <= 4 {
                (40, 22)
            } else {
                (75, 26)
            };
            let base_w = styled.width.max(min_w);
            let base_h = styled.height.max(min_h);
            Size::new(base_w, base_h)
        } else {
            // Qt's `QStyleSheetStyle::sizeFromContents(CT_PushButton)` + `rule.boxSize(sz)`:
            // `min-width` applies to the content box, and padding + border are added around it.
            // `rule.adjustSize(csz)`: `width`/`height` replace the text size as the content size.
            let text_w = style.width.unwrap_or(text_w);
            let text_h = style.height.unwrap_or(text_h);
            let min_w = style.min_width.unwrap_or(0);
            let content_w = text_w.max(min_w);
            let mut w = content_w + pad_h + border_h;
            if let Some(max_w) = style.max_width {
                w = w.min(max_w);
            }

            let min_h = style.min_height.unwrap_or(0);
            let content_h = text_h.max(min_h);
            let mut h = content_h + pad_v + border_v;
            if let Some(max_h) = style.max_height {
                h = h.min(max_h);
            }
            if let Some(min_h) = style.min_height {
                h = h.max(min_h);
            }
            Size::new(w, h)
        }
    }

    /// `QPushButton::minimumSizeHint` returns `sizeHint()` (`qpushbutton.cpp:398-401`).
    fn minimum_size_hint(&self) -> Size {
        self.size_hint()
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

    fn layout(&self) -> Option<&dyn Layout> {
        None
    }

    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }

    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}

    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }

    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }

    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }

    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        Vec::new()
    }

    fn add_child(&mut self, _child: WidgetRef) {}

    fn remove_child(&mut self, _child_id: ObjectId) {}

    fn enter_event(&mut self, _pos: Point) {
        if self.base.is_enabled() && self.state != ButtonState::Pressed {
            self.state = ButtonState::Hovered;
            self.update();
        }
    }

    fn leave_event(&mut self) {
        if self.base.is_enabled() && self.state != ButtonState::Normal {
            self.state = ButtonState::Normal;
            self.update();
        }
    }

    fn mouse_press_event(&mut self, _pos: Point, button: u32, _modifiers: u32) {
        if self.base.is_enabled() && button == 1 {
            self.state = ButtonState::Pressed;
            self.update();
        }
    }

    fn mouse_release_event(&mut self, pos: Point, button: u32, _modifiers: u32) {
        if self.base.is_enabled() && button == 1 && self.state == ButtonState::Pressed {
            let g = self.base.geometry();
            let in_bounds =
                Rect::new(0, 0, g.width, g.height).contains(pos);
            if in_bounds {
                self.state = ButtonState::Hovered;
                self.activate();
            } else {
                self.state = ButtonState::Normal;
            }
            self.update();
        }
    }
    fn focus_policy(&self) -> crate::focus::FocusPolicy {
        self.base.focus_policy()
    }

    fn set_focus_policy(&self, policy: crate::focus::FocusPolicy) {
        self.base.set_focus_policy(policy);
    }

    fn has_focus(&self) -> bool {
        self.base.has_focus()
    }

    fn set_has_focus(&self, focus: bool) {
        self.base.set_has_focus(focus);
    }
    fn accessible_role(&self) -> AccessibleRole {
        AccessibleRole::Button
    }

    fn accessible_name(&self) -> String {
        self.text.clone()
    }

    fn accessible_actions(&self) -> Vec<AccessibleAction> {
        vec![AccessibleAction::Invoke, AccessibleAction::Focus]
    }

    fn perform_accessible_action(&mut self, action: AccessibleAction) -> bool {
        match action {
            AccessibleAction::Invoke if self.base.is_enabled() => {
                self.activate();
                true
            }
            AccessibleAction::Focus if self.base.is_enabled() => {
                self.set_has_focus(true);
                true
            }
            _ => false,
        }
    }

    fn focus_in_event(&mut self, _reason: ferrokit_core::event::FocusReason) {
        self.update();
    }

    fn focus_out_event(&mut self, _reason: ferrokit_core::event::FocusReason) {
        self.update();
    }

    fn key_press_event(&mut self, key: u32, _modifiers: u32, _is_repeat: bool) {
        if self.base.is_enabled()
            && (key == 0x20 || key == 0x0D || key == 0x01000004 || key == 0x01000005)
        {
            self.state = ButtonState::Pressed;
            self.update();
        }
    }

    fn key_release_event(&mut self, key: u32, _modifiers: u32) {
        if self.base.is_enabled()
            && (key == 0x20 || key == 0x0D || key == 0x01000004 || key == 0x01000005)
            && self.state == ButtonState::Pressed
        {
            self.activate();
            self.update();
        }
    }

    fn paint_event(&mut self, painter: &mut Painter) {
        let geom = self.base.geometry();
        let style = self.resolved_style();
        let bg = style.background_color.unwrap_or_else(|| match self.state {
            ButtonState::Normal => self.normal_bg,
            ButtonState::Hovered => self.hover_bg,
            ButtonState::Pressed => self.press_bg,
        });
        let text_color = style.color.unwrap_or(self.text_color);
        let border_color = style.border_color.unwrap_or(self.border_color);
        let border_radius = style.border_radius.unwrap_or(self.border_radius);
        self.style.draw_button(
            painter,
            &ButtonStyleOption {
                rect: RectF::new(0.0, 0.0, geom.width as f32, geom.height as f32),
                text: &self.text,
                font: &self.styled_font(&style),
                state: self.state,
                enabled: self.base.is_enabled(),
                // `QStyleSheetStyle` draws the box of a button the style sheet gives a background
                // or border to, and no focus rectangle.
                focused: self.has_focus()
                    && style.background_color.is_none()
                    && style.border_color.is_none(),
                normal_color: bg,
                hover_color: bg,
                pressed_color: bg,
                text_color,
                border_color,
                border_radius,
                border_width: style
                    .border_width
                    .filter(|w| *w > 0.0 && style.border_style.as_deref() != Some("none")),
            },
        );
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
